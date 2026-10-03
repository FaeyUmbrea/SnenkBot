//! Operational run history stored as one readable JSON file per execution.
//!
//! Trigger metadata intentionally contains identity only. Trigger payloads and
//! credential values belong to the dispatcher and must never be copied here.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::engine::{ScriptActivity, StepTrace};

const DEFAULT_RETENTION: usize = 500;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct TriggerIdentity {
    pub kind: TriggerKind,
    /// A non-secret configured identity (for example, a schedule or webhook ID).
    /// Do not put a payload, credential, URL, or user-provided value here.
    pub id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum HistoryTriggerKind {
    Manual,
    Schedule,
    Event,
    Webhook,
}

pub type TriggerKind = HistoryTriggerKind;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunOutcome {
    Running,
    Succeeded,
    Stopped,
    Cancelled,
    Failed { category: FailureCategory },
    Rejected,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    Definition,
    MissingValue,
    InvalidType,
    Capability,
    Connector,
    Action,
    Timeout,
    Input,
    Limit,
    Other,
}

impl RunOutcome {
    fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct RunRecord {
    pub run_id: String,
    pub workflow_id: String,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub workflow_revision: u64,
    pub trigger: TriggerIdentity,
    /// Milliseconds since the Unix epoch, UTC.
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub started_at_ms: u64,
    /// Milliseconds since the Unix epoch, UTC; absent while the run is active.
    #[cfg_attr(feature = "desktop-contracts", specta(type = Option<specta_typescript::Number>))]
    pub finished_at_ms: Option<u64>,
    pub outcome: RunOutcome,
    /// Script step IDs and outcomes only; script source and values stay private.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_activity: Vec<ScriptActivity>,
    /// Typed execution steps for this run; no script source, values, or failure messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_trace: Vec<StepTrace>,
}

/// Run identity and outcome without the execution trace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct RunSummary {
    pub run_id: String,
    pub workflow_id: String,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub workflow_revision: u64,
    pub trigger: TriggerIdentity,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub started_at_ms: u64,
    #[cfg_attr(feature = "desktop-contracts", specta(type = Option<specta_typescript::Number>))]
    pub finished_at_ms: Option<u64>,
    pub outcome: RunOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistorySummaryEntry {
    Record {
        file_name: String,
        summary: RunSummary,
    },
    Corrupt {
        file_name: String,
        message: String,
    },
}

#[derive(Deserialize)]
struct SummaryRecord {
    run_id: String,
    workflow_id: String,
    workflow_revision: u64,
    trigger: TriggerIdentity,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    outcome: RunOutcome,
    #[serde(default, rename = "script_activity")]
    _script_activity: Discarded<ScriptActivity>,
    #[serde(default, rename = "step_trace")]
    _step_trace: Discarded<StepTrace>,
}

impl From<SummaryRecord> for RunSummary {
    fn from(record: SummaryRecord) -> Self {
        Self {
            run_id: record.run_id,
            workflow_id: record.workflow_id,
            workflow_revision: record.workflow_revision,
            trigger: record.trigger,
            started_at_ms: record.started_at_ms,
            finished_at_ms: record.finished_at_ms,
            outcome: record.outcome,
        }
    }
}

struct Discarded<T>(PhantomData<T>);

impl<T> Default for Discarded<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Discarded<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Sequence<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for Sequence<T> {
            type Value = Discarded<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                // Keep full-record validation, but retain at most one trace entry at a time.
                while sequence.next_element::<T>()?.is_some() {}
                Ok(Discarded(PhantomData))
            }
        }

        deserializer.deserialize_seq(Sequence(PhantomData))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryEntry {
    Record(RunRecord),
    Corrupt { file_name: String, message: String },
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("invalid run ID `{0}`")]
    InvalidRunId(String),
    #[error("I/O error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("history JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("system clock is before the Unix epoch")]
    Clock,
    #[error("run `{0}` is not currently running")]
    NotRunning(String),
    #[error("run `{0}` already exists")]
    AlreadyExists(String),
}

/// A bounded journal of individual workflow runs. Construct with
/// [`RunHistory::new`] using `AppPaths::history_dir()` as the directory.
#[derive(Clone, Debug)]
pub struct RunHistory {
    directory: PathBuf,
    retention: usize,
}

impl RunHistory {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self::with_retention(directory, DEFAULT_RETENTION)
    }

    pub fn with_retention(directory: impl Into<PathBuf>, retention: usize) -> Self {
        Self {
            directory: directory.into(),
            retention: retention.max(1),
        }
    }

    /// Creates a stable ID and persists the active record before dispatch begins.
    pub fn begin(
        &self,
        workflow_id: impl Into<String>,
        workflow_revision: u64,
        trigger: TriggerIdentity,
    ) -> Result<RunRecord, HistoryError> {
        self.create(RunRecord {
            run_id: new_run_id()?,
            workflow_id: workflow_id.into(),
            workflow_revision,
            trigger,
            started_at_ms: now_ms()?,
            finished_at_ms: None,
            outcome: RunOutcome::Running,
            script_activity: Vec::new(),
            step_trace: Vec::new(),
        })
    }

    /// Persists a finished outcome. It always records its own finish time.
    pub fn finish(&self, run_id: &str, outcome: RunOutcome) -> Result<RunRecord, HistoryError> {
        self.finish_with_activity(run_id, outcome, Vec::new())
    }

    pub fn finish_with_activity(
        &self,
        run_id: &str,
        outcome: RunOutcome,
        script_activity: Vec<ScriptActivity>,
    ) -> Result<RunRecord, HistoryError> {
        self.finish_with_trace(run_id, outcome, script_activity, Vec::new())
    }

    pub fn finish_with_trace(
        &self,
        run_id: &str,
        outcome: RunOutcome,
        script_activity: Vec<ScriptActivity>,
        step_trace: Vec<StepTrace>,
    ) -> Result<RunRecord, HistoryError> {
        if outcome.is_running() {
            return Err(HistoryError::NotRunning(run_id.to_owned()));
        }
        let path = self.record_path(run_id)?;
        let mut record = self.read_record(&path)?;
        if !record.outcome.is_running() {
            return Err(HistoryError::NotRunning(run_id.to_owned()));
        }
        record.outcome = outcome;
        record.script_activity = script_activity;
        record.step_trace = step_trace;
        record.finished_at_ms = Some(now_ms()?);
        self.write_record(&path, &record)?;
        self.prune()?;
        Ok(record)
    }

    fn create_record_file(&self, path: &Path, record: &RunRecord) -> Result<(), HistoryError> {
        fs::create_dir_all(&self.directory).map_err(|source| HistoryError::Io {
            path: self.directory.clone(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(record)?;
        let (temp_path, mut file) = staged_file(path)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            match fs::hard_link(&temp_path, path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
            fs::remove_file(&temp_path)?;
            sync_directory(&self.directory)?;
            Ok::<_, io::Error>(())
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temp_path);
            if source.kind() == io::ErrorKind::AlreadyExists {
                return Err(HistoryError::AlreadyExists(record.run_id.clone()));
            }
            return Err(HistoryError::Io {
                path: path.to_owned(),
                source,
            });
        }
        Ok(())
    }

    /// Marks active records from an earlier process as interrupted. Corrupt files
    /// are left byte-for-byte intact and returned alongside valid records.
    pub fn recover(&self) -> Result<Vec<HistoryEntry>, HistoryError> {
        let files = self.json_files()?;
        let mut entries = Vec::with_capacity(files.len());
        for path in files {
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(source) => return Err(HistoryError::Io { path, source }),
            };
            match serde_json::from_slice::<RunRecord>(&bytes) {
                Ok(mut record) => {
                    if record.outcome.is_running() {
                        record.outcome = RunOutcome::Interrupted;
                        record.finished_at_ms = Some(now_ms()?);
                        self.write_record(&path, &record)?;
                    }
                    entries.push(HistoryEntry::Record(record));
                }
                Err(error) => entries.push(HistoryEntry::Corrupt {
                    file_name: path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("unknown.json")
                        .to_owned(),
                    message: error.to_string(),
                }),
            }
        }
        self.prune()?;
        Ok(entries)
    }

    /// Reads the journal without changing active records or removing corrupt files.
    pub fn entries(&self) -> Result<Vec<HistoryEntry>, HistoryError> {
        let files = self.json_files()?;
        let mut entries = Vec::with_capacity(files.len());
        for path in files {
            let bytes = fs::read(&path).map_err(|source| HistoryError::Io {
                path: path.clone(),
                source,
            })?;
            match serde_json::from_slice::<RunRecord>(&bytes) {
                Ok(record) => entries.push(HistoryEntry::Record(record)),
                Err(error) => entries.push(HistoryEntry::Corrupt {
                    file_name: path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("unknown.json")
                        .to_owned(),
                    message: error.to_string(),
                }),
            }
        }
        Ok(entries)
    }

    /// Streams summary fields while validating and discarding individual trace entries.
    /// Like [`Self::entries`], this never recovers active runs or prunes the journal.
    pub fn summaries(&self) -> Result<Vec<HistorySummaryEntry>, HistoryError> {
        let files = self.json_files()?;
        let mut entries = Vec::with_capacity(files.len());
        for path in files {
            let file = File::open(&path).map_err(|source| HistoryError::Io {
                path: path.clone(),
                source,
            })?;
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown.json")
                .to_owned();
            match serde_json::from_reader::<_, SummaryRecord>(BufReader::new(file)) {
                Ok(record) => entries.push(HistorySummaryEntry::Record {
                    file_name,
                    summary: record.into(),
                }),
                Err(error) if error.is_io() => {
                    return Err(HistoryError::Io {
                        path,
                        source: io::Error::new(
                            error.io_error_kind().unwrap_or(io::ErrorKind::Other),
                            "history record could not be read",
                        ),
                    });
                }
                Err(error) => entries.push(HistorySummaryEntry::Corrupt {
                    file_name,
                    message: error.to_string(),
                }),
            }
        }
        Ok(entries)
    }

    pub fn load(&self, run_id: &str) -> Result<RunRecord, HistoryError> {
        let path = self.record_path(run_id)?;
        self.read_record(&path)
    }

    fn create(&self, record: RunRecord) -> Result<RunRecord, HistoryError> {
        let path = self.record_path(&record.run_id)?;
        fs::create_dir_all(&self.directory).map_err(|source| HistoryError::Io {
            path: self.directory.clone(),
            source,
        })?;
        self.create_record_file(&path, &record)?;
        self.prune()?;
        Ok(record)
    }

    fn read_record(&self, path: &Path) -> Result<RunRecord, HistoryError> {
        let bytes = fs::read(path).map_err(|source| HistoryError::Io {
            path: path.to_owned(),
            source,
        })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn write_record(&self, path: &Path, record: &RunRecord) -> Result<(), HistoryError> {
        fs::create_dir_all(&self.directory).map_err(|source| HistoryError::Io {
            path: self.directory.clone(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(record)?;
        let (temp_path, mut file) = staged_file(path)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            replace_file(&temp_path, path)?;
            sync_directory(&self.directory)?;
            Ok::<_, io::Error>(())
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temp_path);
            return Err(HistoryError::Io {
                path: path.to_owned(),
                source,
            });
        }
        Ok(())
    }

    fn json_files(&self) -> Result<Vec<PathBuf>, HistoryError> {
        let directory = match fs::read_dir(&self.directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(HistoryError::Io {
                    path: self.directory.clone(),
                    source,
                });
            }
        };
        let mut files = Vec::new();
        for entry in directory {
            let entry = entry.map_err(|source| HistoryError::Io {
                path: self.directory.clone(),
                source,
            })?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }

    fn prune(&self) -> Result<(), HistoryError> {
        let mut records = Vec::new();
        for path in self.json_files()? {
            if let Ok(record) = self.read_record(&path)
                && !record.outcome.is_running()
            {
                records.push((record.started_at_ms, path));
            }
        }
        records.sort_by_key(|(started, path)| (*started, path.clone()));
        let remove_count = records.len().saturating_sub(self.retention);
        for (_, path) in records.into_iter().take(remove_count) {
            fs::remove_file(&path).map_err(|source| HistoryError::Io { path, source })?;
        }
        Ok(())
    }

    fn record_path(&self, run_id: &str) -> Result<PathBuf, HistoryError> {
        if run_id.is_empty()
            || run_id.len() > 128
            || !run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(HistoryError::InvalidRunId(run_id.to_owned()));
        }
        Ok(self.directory.join(format!("{run_id}.json")))
    }
}

fn new_run_id() -> Result<String, HistoryError> {
    let now = now_ms()?;
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    Ok(format!("{now}-{}-{sequence}", std::process::id()))
}

fn now_ms() -> Result<u64, HistoryError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HistoryError::Clock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| HistoryError::Clock)
}

fn staged_file(path: &Path) -> Result<(PathBuf, File), HistoryError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for _ in 0..32 {
        let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".history-{}-{sequence}.tmp", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(HistoryError::Io {
                    path: temp_path,
                    source,
                });
            }
        }
    }
    Err(HistoryError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::AlreadyExists, "cannot allocate staged file"),
    })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // Both names are NUL terminated UTF-16 paths and refer to files in one directory.
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::{HistoryEntry, RunHistory, RunOutcome, RunRecord, TriggerIdentity, TriggerKind};
    use crate::engine::{StepTrace, StepTraceKind, StepTraceOutcome};

    fn trigger() -> TriggerIdentity {
        TriggerIdentity {
            kind: TriggerKind::Manual,
            id: None,
        }
    }

    #[test]
    fn summaries_stream_large_traces_and_unknown_fields_without_retaining_them() {
        use std::io::{BufWriter, Write};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.json");
        let mut file = BufWriter::new(std::fs::File::create(&path).unwrap());
        file.write_all(br#"{"run_id":"large","workflow_id":"workflow","workflow_revision":2,"trigger":{"kind":"manual","id":null},"started_at_ms":10,"finished_at_ms":20,"outcome":{"status":"succeeded"},"step_trace":["#).unwrap();
        let trace = StepTrace {
            sequence: 1,
            parent_sequence: None,
            workflow_id: "workflow".into(),
            workflow_revision: 2,
            step_id: "delay".into(),
            kind: StepTraceKind::Delay,
            started_at_ms: 10,
            finished_at_ms: 20,
            duration_ms: 10,
            outcome: StepTraceOutcome::Succeeded,
        };
        for index in 0..20_000 {
            if index != 0 {
                file.write_all(b",").unwrap();
            }
            serde_json::to_writer(&mut file, &trace).unwrap();
        }
        file.write_all(br#"],"future_field":["#).unwrap();
        for index in 0..100_000 {
            if index != 0 {
                file.write_all(b",").unwrap();
            }
            file.write_all(b"0").unwrap();
        }
        file.write_all(b"]}").unwrap();
        file.flush().unwrap();
        drop(file);
        let before = std::fs::metadata(&path).unwrap();
        let entries = RunHistory::new(dir.path()).summaries().unwrap();
        assert!(
            matches!(&entries[..], [super::HistorySummaryEntry::Record { summary, .. }]
            if summary.run_id == "large" && summary.workflow_revision == 2)
        );
        assert_eq!(std::fs::metadata(path).unwrap().len(), before.len());
    }

    #[test]
    fn summaries_preserve_full_record_validation_for_discarded_traces() {
        let dir = tempfile::tempdir().unwrap();
        let history = RunHistory::new(dir.path());
        let active = history.begin("workflow", 1, trigger()).unwrap();
        let mut value = serde_json::to_value(&active).unwrap();
        let path = dir.path().join(format!("{}.json", active.run_id));
        for (field, invalid) in [
            ("step_trace", serde_json::json!([{}])),
            ("step_trace", serde_json::json!(null)),
            (
                "script_activity",
                serde_json::json!([{"workflow_id": "workflow", "step_id": "script", "outcome": "unknown"}]),
            ),
        ] {
            value[field] = invalid;
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(matches!(
                &history.summaries().unwrap()[..],
                [super::HistorySummaryEntry::Corrupt { .. }]
            ));
            assert!(matches!(
                &history.entries().unwrap()[..],
                [HistoryEntry::Corrupt { .. }]
            ));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            value.as_object_mut().unwrap().remove(field);
        }
    }

    #[test]
    fn summary_reads_do_not_create_a_missing_history_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent");
        assert!(RunHistory::new(&path).summaries().unwrap().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn record_round_trips_as_readable_json() {
        let dir = tempfile::tempdir().unwrap();
        let history = RunHistory::new(dir.path());
        let started = history.begin("daily", 7, trigger()).unwrap();
        let finished = history
            .finish(&started.run_id, RunOutcome::Succeeded)
            .unwrap();
        let loaded = history.load(&started.run_id).unwrap();

        assert_eq!(loaded, finished);
        assert_eq!(loaded.workflow_id, "daily");
        assert_eq!(loaded.workflow_revision, 7);
        assert!(loaded.finished_at_ms.is_some());
        let mut legacy = serde_json::to_value(&loaded).unwrap();
        legacy.as_object_mut().unwrap().remove("script_activity");
        legacy.as_object_mut().unwrap().remove("step_trace");
        let legacy: RunRecord = serde_json::from_value(legacy).unwrap();
        assert!(legacy.script_activity.is_empty());
        assert!(legacy.step_trace.is_empty());
        let text =
            std::fs::read_to_string(dir.path().join(format!("{}.json", loaded.run_id))).unwrap();
        assert!(text.contains("\n  \"workflow_id\""));
    }

    #[test]
    fn completed_step_trace_round_trips_without_values_or_messages() {
        let dir = tempfile::tempdir().unwrap();
        let history = RunHistory::new(dir.path());
        let started = history.begin("workflow", 2, trigger()).unwrap();
        let trace = StepTrace {
            sequence: 1,
            parent_sequence: None,
            workflow_id: "workflow".into(),
            workflow_revision: 2,
            step_id: "first".into(),
            kind: StepTraceKind::Delay,
            started_at_ms: 10,
            finished_at_ms: 20,
            duration_ms: 10,
            outcome: StepTraceOutcome::Succeeded,
        };
        history
            .finish_with_trace(
                &started.run_id,
                RunOutcome::Succeeded,
                Vec::new(),
                vec![trace.clone()],
            )
            .unwrap();
        let loaded = history.load(&started.run_id).unwrap();
        assert_eq!(loaded.step_trace, vec![trace]);
        let stored =
            std::fs::read_to_string(dir.path().join(format!("{}.json", started.run_id))).unwrap();
        assert!(stored.contains("\"step_trace\""));
        assert!(!stored.contains("message"));
    }

    #[test]
    fn restart_marks_active_record_interrupted_without_resuming_it() {
        let dir = tempfile::tempdir().unwrap();
        let first_process = RunHistory::new(dir.path());
        let active = first_process.begin("daily", 1, trigger()).unwrap();

        let recovered = RunHistory::new(dir.path()).recover().unwrap();
        assert!(matches!(&recovered[0], HistoryEntry::Record(record)
            if record.run_id == active.run_id
                && record.outcome == RunOutcome::Interrupted
                && record.finished_at_ms.is_some()));
        assert!(matches!(
            RunHistory::new(dir.path()).finish(&active.run_id, RunOutcome::Succeeded),
            Err(super::HistoryError::NotRunning(_))
        ));
    }

    #[test]
    fn malformed_record_is_preserved_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        let bytes = b"{ definitely not json";
        std::fs::write(&path, bytes).unwrap();

        let entries = RunHistory::new(dir.path()).recover().unwrap();
        assert!(
            matches!(&entries[0], HistoryEntry::Corrupt { file_name, .. } if file_name == "broken.json")
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn retention_removes_oldest_valid_records_and_keeps_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let history = RunHistory::with_retention(dir.path(), 2);
        let one = history.begin("wf", 1, trigger()).unwrap();
        history.finish(&one.run_id, RunOutcome::Succeeded).unwrap();
        let two = history.begin("wf", 1, trigger()).unwrap();
        history.finish(&two.run_id, RunOutcome::Succeeded).unwrap();
        let three = history.begin("wf", 1, trigger()).unwrap();
        history
            .finish(&three.run_id, RunOutcome::Succeeded)
            .unwrap();
        std::fs::write(dir.path().join("corrupt.json"), b"bad").unwrap();

        assert!(history.load(&one.run_id).is_err());
        assert!(history.load(&two.run_id).is_ok());
        assert!(history.load(&three.run_id).is_ok());
        assert_eq!(
            std::fs::read(dir.path().join("corrupt.json")).unwrap(),
            b"bad"
        );
    }

    #[test]
    fn retention_never_removes_active_runs_and_recovery_interrupts_them() {
        let dir = tempfile::tempdir().unwrap();
        let history = RunHistory::with_retention(dir.path(), 1);
        let first = history.begin("wf", 1, trigger()).unwrap();
        let second = history.begin("wf", 1, trigger()).unwrap();

        assert_eq!(
            history.load(&first.run_id).unwrap().outcome,
            RunOutcome::Running
        );
        assert_eq!(
            history.load(&second.run_id).unwrap().outcome,
            RunOutcome::Running
        );
        history
            .finish(&first.run_id, RunOutcome::Succeeded)
            .unwrap();
        assert_eq!(
            history.load(&second.run_id).unwrap().outcome,
            RunOutcome::Running
        );

        let recovered = history.recover().unwrap();
        assert!(matches!(
            recovered.iter().find(|entry| matches!(entry,
                HistoryEntry::Record(record) if record.run_id == second.run_id)),
            Some(HistoryEntry::Record(record)) if record.outcome == RunOutcome::Interrupted
        ));
    }

    #[test]
    fn graceful_interruption_is_terminal_and_does_not_get_recovered_again() {
        let dir = tempfile::tempdir().unwrap();
        let history = RunHistory::new(dir.path());
        let active = history.begin("wf", 1, trigger()).unwrap();

        let interrupted = history
            .finish(&active.run_id, RunOutcome::Interrupted)
            .unwrap();
        assert_eq!(interrupted.outcome, RunOutcome::Interrupted);
        assert!(interrupted.finished_at_ms.is_some());
        assert!(matches!(
            history.recover().unwrap().as_slice(),
            [HistoryEntry::Record(record)] if record.outcome == RunOutcome::Interrupted
        ));
    }
}
