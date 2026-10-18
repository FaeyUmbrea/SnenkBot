use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;

use serde::{Deserialize, Serialize};
use specta::Type;

use snenk_bot::history::{
    HistoryError, HistorySummaryEntry, RunHistory, RunOutcome, RunRecord, RunSummary, TriggerKind,
};

pub const MAX_HISTORY_PAGE_SIZE: u32 = 100;
const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_CURSOR_LENGTH: usize = 4096;

#[derive(Clone, Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub filter: HistoryFilter,
}

fn default_page_size() -> u32 {
    DEFAULT_PAGE_SIZE
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct HistoryFilter {
    pub workflow_id: Option<String>,
    pub outcome: Option<HistoryOutcome>,
    pub trigger_kind: Option<TriggerKind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOutcome {
    Running,
    Succeeded,
    Stopped,
    Cancelled,
    Failed,
    Rejected,
    Interrupted,
}

impl From<&RunOutcome> for HistoryOutcome {
    fn from(outcome: &RunOutcome) -> Self {
        match outcome {
            RunOutcome::Running => Self::Running,
            RunOutcome::Succeeded => Self::Succeeded,
            RunOutcome::Stopped => Self::Stopped,
            RunOutcome::Cancelled => Self::Cancelled,
            RunOutcome::Failed { .. } => Self::Failed,
            RunOutcome::Rejected => Self::Rejected,
            RunOutcome::Interrupted => Self::Interrupted,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistorySummary {
    Record { summary: RunSummary },
    Corrupt { file_name: String, message: String },
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct HistoryPage {
    pub entries: Vec<HistorySummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HistoryErrorCode {
    Busy,
    InvalidRequest,
    InvalidCursor,
    StaleCursor,
    Unavailable,
    RunUnavailable,
    CorruptRecord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Type)]
pub struct HistoryQueryError {
    pub code: HistoryErrorCode,
    pub message: String,
}

impl HistoryQueryError {
    fn new(code: HistoryErrorCode, message: &str) -> Self {
        Self {
            code,
            message: message.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SortKey {
    started_at_ms: u64,
    run_id: String,
    file_name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    snapshot: u64,
    filter: HistoryFilter,
    after: SortKey,
}

/// Read-only history queries. Run each query on a blocking worker in the command layer.
#[derive(Clone, Debug)]
pub struct DesktopHistory {
    history: RunHistory,
}

impl DesktopHistory {
    pub fn new(history: RunHistory) -> Self {
        Self { history }
    }

    /// Corrupt records remain visible under all filters because their metadata is unknown.
    /// Cursors identify the ordered summary snapshot; changes require restarting at page one.
    pub fn page(&self, query: HistoryQuery) -> Result<HistoryPage, HistoryQueryError> {
        validate_query(&query)?;
        let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
        if cursor
            .as_ref()
            .is_some_and(|cursor| cursor.filter != query.filter)
        {
            return Err(invalid_cursor());
        }
        let mut entries: Vec<_> = self
            .history
            .summaries()
            .map_err(|_| unavailable())?
            .into_iter()
            .map(|entry| match entry {
                HistorySummaryEntry::Record { file_name, summary }
                    if file_name != format!("{}.json", summary.run_id) =>
                {
                    HistorySummaryEntry::Corrupt {
                        file_name,
                        message: "Run identity does not match its journal file".into(),
                    }
                }
                entry => entry,
            })
            .collect();
        entries.sort_by(|left, right| {
            let left = sort_key(left);
            let right = sort_key(right);
            right
                .started_at_ms
                .cmp(&left.started_at_ms)
                .then_with(|| left.run_id.cmp(&right.run_id))
                .then_with(|| left.file_name.cmp(&right.file_name))
        });
        // Fingerprint before filtering so additions/removals cannot silently change a cursor's scope.
        let snapshot = snapshot_fingerprint(&entries);
        entries.retain(|entry| matches_filter(entry, &query.filter));
        let start = match cursor {
            Some(cursor) => {
                if cursor.snapshot != snapshot {
                    return Err(HistoryQueryError::new(
                        HistoryErrorCode::StaleCursor,
                        "Run history changed. Refresh the history to continue browsing.",
                    ));
                }
                entries
                    .iter()
                    .position(|entry| sort_key(entry) == cursor.after)
                    .ok_or_else(invalid_cursor)?
                    + 1
            }
            None => 0,
        };
        let end = (start + query.page_size as usize).min(entries.len());
        let next_cursor = if end < entries.len() {
            Some(encode_cursor(&Cursor {
                version: 1,
                snapshot,
                filter: query.filter,
                after: sort_key(&entries[end - 1]),
            })?)
        } else {
            None
        };
        Ok(HistoryPage {
            entries: entries
                .into_iter()
                .skip(start)
                .take(end - start)
                .map(summary)
                .collect(),
            next_cursor,
        })
    }

    /// Loads execution traces only for the selected run, using the journal's existing ID validation.
    pub fn inspect(&self, run_id: &str) -> Result<RunRecord, HistoryQueryError> {
        let record = self.history.load(run_id).map_err(|error| match error {
            HistoryError::InvalidRunId(_) => {
                HistoryQueryError::new(HistoryErrorCode::InvalidRequest, "Choose a valid run ID.")
            }
            HistoryError::Json(_) => HistoryQueryError::new(
                HistoryErrorCode::CorruptRecord,
                "This run record could not be read.",
            ),
            HistoryError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                HistoryQueryError::new(
                    HistoryErrorCode::RunUnavailable,
                    "This run is no longer available in history.",
                )
            }
            _ => unavailable(),
        })?;
        if record.run_id != run_id {
            return Err(HistoryQueryError::new(
                HistoryErrorCode::CorruptRecord,
                "This run record could not be read.",
            ));
        }
        Ok(record)
    }
}

fn validate_query(query: &HistoryQuery) -> Result<(), HistoryQueryError> {
    if query.page_size == 0 || query.page_size > MAX_HISTORY_PAGE_SIZE {
        return Err(HistoryQueryError::new(
            HistoryErrorCode::InvalidRequest,
            "Choose a history page size between 1 and 100.",
        ));
    }
    if query
        .filter
        .workflow_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 1024)
    {
        return Err(HistoryQueryError::new(
            HistoryErrorCode::InvalidRequest,
            "Choose a valid workflow ID for the history filter.",
        ));
    }
    Ok(())
}

fn sort_key(entry: &HistorySummaryEntry) -> SortKey {
    match entry {
        HistorySummaryEntry::Record { file_name, summary } => SortKey {
            started_at_ms: summary.started_at_ms,
            run_id: summary.run_id.clone(),
            file_name: file_name.clone(),
        },
        HistorySummaryEntry::Corrupt { file_name, .. } => SortKey {
            started_at_ms: 0,
            run_id: file_name.clone(),
            file_name: file_name.clone(),
        },
    }
}

fn matches_filter(entry: &HistorySummaryEntry, filter: &HistoryFilter) -> bool {
    let HistorySummaryEntry::Record { summary, .. } = entry else {
        return true;
    };
    filter
        .workflow_id
        .as_ref()
        .is_none_or(|id| id == &summary.workflow_id)
        && filter
            .outcome
            .is_none_or(|outcome| outcome == HistoryOutcome::from(&summary.outcome))
        && filter
            .trigger_kind
            .is_none_or(|kind| kind == summary.trigger.kind)
}

fn snapshot_fingerprint(entries: &[HistorySummaryEntry]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for entry in entries {
        let key = sort_key(entry);
        key.started_at_ms.hash(&mut hasher);
        key.run_id.hash(&mut hasher);
        key.file_name.hash(&mut hasher);
        match entry {
            HistorySummaryEntry::Record { summary, .. } => {
                // Outcome/trigger changes can alter filtered membership without changing the sort key.
                serde_json::to_vec(summary)
                    .expect("run summary is serializable")
                    .hash(&mut hasher);
            }
            HistorySummaryEntry::Corrupt { .. } => false.hash(&mut hasher),
        }
    }
    hasher.finish()
}

fn summary(entry: HistorySummaryEntry) -> HistorySummary {
    match entry {
        HistorySummaryEntry::Record { summary, .. } => HistorySummary::Record { summary },
        HistorySummaryEntry::Corrupt { file_name, .. } => HistorySummary::Corrupt {
            file_name,
            message: "This run record could not be read.".to_owned(),
        },
    }
}

fn encode_cursor(cursor: &Cursor) -> Result<String, HistoryQueryError> {
    let bytes = serde_json::to_vec(cursor).map_err(|_| unavailable())?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let encoded: String = bytes
        .iter()
        .flat_map(|byte| {
            [
                HEX[(byte >> 4) as usize] as char,
                HEX[(byte & 15) as usize] as char,
            ]
        })
        .collect();
    if encoded.len() > MAX_CURSOR_LENGTH {
        return Err(unavailable());
    }
    Ok(encoded)
}

fn decode_cursor(encoded: &str) -> Result<Cursor, HistoryQueryError> {
    if encoded.is_empty() || encoded.len() > MAX_CURSOR_LENGTH || !encoded.len().is_multiple_of(2) {
        return Err(invalid_cursor());
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().as_chunks::<2>().0 {
        let high = hex_digit(pair[0]).ok_or_else(invalid_cursor)?;
        let low = hex_digit(pair[1]).ok_or_else(invalid_cursor)?;
        bytes.push(high * 16 + low);
    }
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())?;
    if cursor.version != 1 {
        return Err(invalid_cursor());
    }
    Ok(cursor)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn invalid_cursor() -> HistoryQueryError {
    HistoryQueryError::new(
        HistoryErrorCode::InvalidCursor,
        "This history cursor is invalid. Refresh the history to continue browsing.",
    )
}

fn unavailable() -> HistoryQueryError {
    HistoryQueryError::new(
        HistoryErrorCode::Unavailable,
        "Run history could not be read.",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use snenk_bot::engine::{StepTrace, StepTraceKind, StepTraceOutcome};
    use snenk_bot::history::TriggerIdentity;

    use super::*;

    fn record(id: &str, started: u64) -> RunRecord {
        RunRecord {
            run_id: id.to_owned(),
            workflow_id: "workflow".to_owned(),
            workflow_revision: 1,
            trigger: TriggerIdentity {
                kind: TriggerKind::Manual,
                id: None,
            },
            started_at_ms: started,
            finished_at_ms: Some(started + 10),
            outcome: RunOutcome::Succeeded,
            script_activity: Vec::new(),
            step_trace: Vec::new(),
        }
    }

    fn write(directory: &Path, record: &RunRecord) {
        fs::write(
            directory.join(format!("{}.json", record.run_id)),
            serde_json::to_vec(record).unwrap(),
        )
        .unwrap();
    }

    fn query(page_size: u32, cursor: Option<String>) -> HistoryQuery {
        HistoryQuery {
            page_size,
            cursor,
            filter: HistoryFilter::default(),
        }
    }

    fn ids(page: &HistoryPage) -> Vec<&str> {
        page.entries
            .iter()
            .map(|entry| match entry {
                HistorySummary::Record { summary } => summary.run_id.as_str(),
                HistorySummary::Corrupt { file_name, .. } => file_name.as_str(),
            })
            .collect()
    }

    #[test]
    fn pages_preserve_newest_first_order_and_equal_time_run_id_order() {
        let directory = tempfile::tempdir().unwrap();
        for (id, started) in [("older", 10), ("z", 30), ("a", 30), ("middle", 20)] {
            write(directory.path(), &record(id, started));
        }
        fs::write(directory.path().join("corrupt.json"), b"invalid").unwrap();
        let history = DesktopHistory::new(RunHistory::new(directory.path()));
        let first = history.page(query(2, None)).unwrap();
        assert_eq!(ids(&first), ["a", "z"]);
        let second = history.page(query(2, first.next_cursor)).unwrap();
        assert_eq!(ids(&second), ["middle", "older"]);
        let last = history.page(query(2, second.next_cursor)).unwrap();
        assert_eq!(ids(&last), ["corrupt.json"]);
        assert!(last.next_cursor.is_none());
        assert_eq!(
            fs::read(directory.path().join("corrupt.json")).unwrap(),
            b"invalid"
        );
    }

    #[test]
    fn filters_match_workflow_outcome_and_trigger_while_preserving_corrupt_records() {
        let directory = tempfile::tempdir().unwrap();
        let mut selected = record("selected", 30);
        selected.workflow_id = "workflow.with.dots".to_owned();
        selected.outcome = RunOutcome::Running;
        selected.finished_at_ms = None;
        selected.trigger.kind = TriggerKind::Schedule;
        write(directory.path(), &selected);
        for (id, workflow, outcome, trigger) in [
            (
                "other_workflow",
                "workflow",
                RunOutcome::Running,
                TriggerKind::Schedule,
            ),
            (
                "other_outcome",
                "workflow.with.dots",
                RunOutcome::Succeeded,
                TriggerKind::Schedule,
            ),
            (
                "other_trigger",
                "workflow.with.dots",
                RunOutcome::Running,
                TriggerKind::Manual,
            ),
        ] {
            let mut other = record(id, 20);
            other.workflow_id = workflow.to_owned();
            other.outcome = outcome;
            other.trigger.kind = trigger;
            write(directory.path(), &other);
        }
        fs::write(
            directory.path().join("broken.json"),
            b"{ secret fixture value",
        )
        .unwrap();
        let history = DesktopHistory::new(RunHistory::new(directory.path()));
        let mut request = query(100, None);
        request.filter = HistoryFilter {
            workflow_id: Some("workflow.with.dots".to_owned()),
            outcome: Some(HistoryOutcome::Running),
            trigger_kind: Some(TriggerKind::Schedule),
        };
        let page = history.page(request).unwrap();
        assert_eq!(ids(&page), ["selected", "broken.json"]);
        assert!(
            matches!(&page.entries[1], HistorySummary::Corrupt { message, .. }
            if message == "This run record could not be read.")
        );
        // Listing must neither finish active runs nor remove unknown files.
        assert_eq!(history.inspect("selected").unwrap(), selected);
        assert_eq!(
            fs::read(directory.path().join("broken.json")).unwrap(),
            b"{ secret fixture value"
        );
    }

    #[test]
    fn changed_history_requires_refresh_instead_of_shifting_page_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        for (id, started) in [("one", 30), ("two", 20), ("three", 10)] {
            write(directory.path(), &record(id, started));
        }
        let history = DesktopHistory::new(RunHistory::new(directory.path()));
        let first = history.page(query(1, None)).unwrap();
        let cursor = first.next_cursor.unwrap();
        write(directory.path(), &record("new", 40));
        assert_eq!(
            history.page(query(1, Some(cursor))).unwrap_err().code,
            HistoryErrorCode::StaleCursor
        );
        let refreshed = history.page(query(2, None)).unwrap();
        assert_eq!(ids(&refreshed), ["new", "one"]);
        let cursor = refreshed.next_cursor.unwrap();
        fs::remove_file(directory.path().join("two.json")).unwrap();
        assert_eq!(
            history.page(query(2, Some(cursor))).unwrap_err().code,
            HistoryErrorCode::StaleCursor
        );
    }

    #[test]
    fn changed_filtered_membership_invalidates_a_cursor() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), &record("one", 30));
        write(directory.path(), &record("two", 20));
        let history = DesktopHistory::new(RunHistory::new(directory.path()));
        let mut request = query(1, None);
        request.filter.outcome = Some(HistoryOutcome::Succeeded);
        request.cursor = history.page(request.clone()).unwrap().next_cursor;
        let mut changed = record("two", 20);
        changed.outcome = RunOutcome::Cancelled;
        write(directory.path(), &changed);
        assert_eq!(
            history.page(request).unwrap_err().code,
            HistoryErrorCode::StaleCursor
        );
    }

    #[test]
    fn invalid_requests_and_cursors_fail_before_returning_a_page() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), &record("one", 30));
        write(directory.path(), &record("two", 20));
        let history = DesktopHistory::new(RunHistory::new(directory.path()));
        for size in [0, MAX_HISTORY_PAGE_SIZE + 1, u32::MAX] {
            assert_eq!(
                history.page(query(size, None)).unwrap_err().code,
                HistoryErrorCode::InvalidRequest
            );
        }
        for cursor in [
            "".to_owned(),
            "not a cursor".to_owned(),
            "00".to_owned(),
            "a".repeat(MAX_CURSOR_LENGTH + 1),
        ] {
            assert_eq!(
                history.page(query(1, Some(cursor))).unwrap_err().code,
                HistoryErrorCode::InvalidCursor
            );
        }
        let mut request = query(1, history.page(query(1, None)).unwrap().next_cursor);
        request.filter.outcome = Some(HistoryOutcome::Succeeded);
        assert_eq!(
            history.page(request).unwrap_err().code,
            HistoryErrorCode::InvalidCursor
        );
        let first = history.page(query(1, None)).unwrap();
        let mut decoded = decode_cursor(first.next_cursor.as_deref().unwrap()).unwrap();
        decoded.after.run_id = "missing".to_owned();
        assert_eq!(
            history
                .page(query(1, Some(encode_cursor(&decoded).unwrap())))
                .unwrap_err()
                .code,
            HistoryErrorCode::InvalidCursor
        );
    }

    #[test]
    fn selected_detail_loads_only_the_requested_full_record_and_redacts_errors() {
        let directory = tempfile::tempdir().unwrap();
        let mut selected = record("selected", 30);
        selected.step_trace.push(StepTrace {
            sequence: 1,
            parent_sequence: None,
            workflow_id: "workflow".to_owned(),
            workflow_revision: 1,
            step_id: "delay".to_owned(),
            kind: StepTraceKind::Delay,
            started_at_ms: 30,
            finished_at_ms: 40,
            duration_ms: 10,
            outcome: StepTraceOutcome::Succeeded,
        });
        write(directory.path(), &selected);
        fs::write(directory.path().join("broken.json"), b"bad").unwrap();
        let history = DesktopHistory::new(RunHistory::new(directory.path()));
        assert_eq!(history.inspect("selected").unwrap(), selected);
        for (id, code) in [
            ("../secret", HistoryErrorCode::InvalidRequest),
            ("missing", HistoryErrorCode::RunUnavailable),
            ("broken", HistoryErrorCode::CorruptRecord),
        ] {
            let error = history.inspect(id).unwrap_err();
            assert_eq!(error.code, code);
            assert!(!error.message.contains(directory.path().to_str().unwrap()));
            assert!(!error.message.contains(id));
        }
    }

    #[test]
    fn mismatched_file_identity_cannot_inspect_an_unrelated_run() {
        let directory = tempfile::tempdir().unwrap();
        let selected = record("selected", 30);
        write(directory.path(), &selected);
        fs::write(
            directory.path().join("mismatched.json"),
            serde_json::to_vec(&selected).unwrap(),
        )
        .unwrap();
        let history = DesktopHistory::new(RunHistory::new(directory.path()));
        let page = history.page(query(100, None)).unwrap();
        assert_eq!(ids(&page), ["selected", "mismatched.json"]);
        assert!(matches!(page.entries[1], HistorySummary::Corrupt { .. }));
        assert_eq!(
            history.inspect("mismatched").unwrap_err().code,
            HistoryErrorCode::CorruptRecord
        );
        assert_eq!(history.inspect("selected").unwrap(), selected);
    }
}
