use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::history::{HistoryEntry, RunHistory, RunOutcome, TriggerKind};
use crate::paths::AppPaths;
use crate::runtime::RuntimeSpawner;
use crate::workflows::{WorkflowDefinitionEntry, WorkflowRepository};

use super::{AppWindow, HistoryRow, HistoryStep};
use crate::engine::{FailureKind, StepTrace, StepTraceKind, StepTraceOutcome};
use crate::schema::ConfigSchema;

/// Connects the read-only run-history view to persisted records.
pub fn connect_run_history(
    window: &AppWindow,
    paths: AppPaths,
    spawner: RuntimeSpawner,
    schemas: Vec<&'static ConfigSchema>,
) {
    let history = RunHistory::new(paths.history_dir());
    let repository = WorkflowRepository::new(&paths);
    let generation = Arc::new(AtomicU64::new(0));

    let weak = window.as_weak();
    let request_generation = Arc::clone(&generation);
    window.on_refresh_history(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let request = request_generation.fetch_add(1, Ordering::Relaxed) + 1;
        window.set_history_loading(true);
        window.set_history_error("".into());

        let history = history.clone();
        let repository = repository.clone();
        let weak = window.as_weak();
        let current_generation = Arc::clone(&request_generation);
        let schemas = schemas.clone();
        let task = spawner.spawn_task("read run history", move |cancel| async move {
            let worker = tokio::task::spawn_blocking(move || load_history(&history, &repository));
            let result = tokio::select! {
                _ = cancel.cancelled() => return Ok::<(), std::convert::Infallible>(()),
                result = worker => match result {
                    Ok(result) => result,
                    Err(error) => Err(format!("History worker stopped: {error}")),
                },
            };
            let _ = weak.upgrade_in_event_loop(move |window| {
                if current_generation.load(Ordering::Relaxed) != request {
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        let rows = history_rows(snapshot, &schemas);
                        let selected = window.get_selected_history_run();
                        if let Some(record) = rows.iter().find(|row| row.run_id == selected) {
                            window.set_selected_history_record(record.clone());
                        }
                        window.set_run_history(ModelRc::new(VecModel::from(rows)));
                        window.set_history_error("".into());
                    }
                    Err(error) => window.set_history_error(error.into()),
                }
                window.set_history_loading(false);
            });
            Ok::<(), std::convert::Infallible>(())
        });
        if let Err(error) = task {
            window.set_history_loading(false);
            window.set_history_error(error.to_string().into());
        }
    });

    window.invoke_refresh_history();
}

fn load_history(
    history: &RunHistory,
    repository: &WorkflowRepository,
) -> Result<HistorySnapshot, String> {
    let titles = repository
        .list_definitions()
        .map(|entries| {
            entries
                .into_iter()
                .filter_map(|entry| match entry {
                    WorkflowDefinitionEntry::Available { id, definition } => {
                        Some((id, definition.title().to_owned()))
                    }
                    WorkflowDefinitionEntry::Unavailable { .. } => None,
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let entries = history.entries().map_err(|error| error.to_string())?;
    Ok(HistorySnapshot { titles, entries })
}

struct HistorySnapshot {
    titles: HashMap<String, String>,
    entries: Vec<HistoryEntry>,
}

fn history_rows(snapshot: HistorySnapshot, schemas: &[&ConfigSchema]) -> Vec<HistoryRow> {
    let HistorySnapshot { titles, entries } = snapshot;
    let mut rows = entries
        .into_iter()
        .map(|entry| match entry {
            HistoryEntry::Record(record) => {
                let title = titles
                    .get(&record.workflow_id)
                    .cloned()
                    .unwrap_or_else(|| record.workflow_id.clone());
                let duration = record
                    .finished_at_ms
                    .map(|finished| format_duration(finished.saturating_sub(record.started_at_ms)))
                    .unwrap_or_else(|| "Running".to_owned());
                let script_activity = record
                    .script_activity
                    .iter()
                    .map(|activity| {
                        format!(
                            "{} · {}: {}",
                            activity.workflow_id,
                            activity.step_id,
                            match activity.outcome {
                                crate::engine::ScriptOutcome::Succeeded => "Succeeded",
                                crate::engine::ScriptOutcome::Failed => "Failed",
                                crate::engine::ScriptOutcome::Cancelled => "Cancelled",
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let outcome = outcome_label(&record.outcome);
                let error = match record.outcome {
                    RunOutcome::Failed { category } => {
                        format!("Failure category: {}", failure_category_label(category))
                    }
                    _ => String::new(),
                };
                let trigger_id = record.trigger.id.unwrap_or_default();
                let trigger = trigger_label(record.trigger.kind, &trigger_id);
                (
                    record.started_at_ms,
                    HistoryRow {
                        run_id: record.run_id.into(),
                        workflow_id: record.workflow_id.into(),
                        title: title.into(),
                        outcome: outcome.into(),
                        trigger: trigger.into(),
                        trigger_id: trigger_id.into(),
                        started_at: format_timestamp(record.started_at_ms).into(),
                        duration: duration.into(),
                        workflow_revision: record.workflow_revision.to_string().into(),
                        script_activity: script_activity.into(),
                        steps: ModelRc::new(VecModel::from(trace_rows(
                            &record.step_trace,
                            schemas,
                        ))),
                        error: error.into(),
                        is_corrupt: false,
                    },
                )
            }
            HistoryEntry::Corrupt { file_name, message } => (
                0,
                HistoryRow {
                    run_id: file_name.into(),
                    workflow_id: "".into(),
                    title: "Unreadable record".into(),
                    outcome: "Corrupt JSON".into(),
                    trigger: "—".into(),
                    trigger_id: "".into(),
                    started_at: "—".into(),
                    duration: "—".into(),
                    workflow_revision: "".into(),
                    script_activity: "".into(),
                    steps: ModelRc::new(VecModel::from(Vec::<HistoryStep>::new())),
                    error: message.into(),
                    is_corrupt: true,
                },
            ),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|(left_time, left), (right_time, right)| {
        right_time
            .cmp(left_time)
            .then_with(|| left.run_id.as_str().cmp(right.run_id.as_str()))
    });
    rows.into_iter().map(|(_, row)| row).collect()
}

fn trace_rows(trace: &[StepTrace], schemas: &[&ConfigSchema]) -> Vec<HistoryStep> {
    let mut depths: HashMap<u64, i32> = HashMap::new();
    trace
        .iter()
        .map(|step| {
            let depth = step
                .parent_sequence
                .and_then(|parent| depths.get(&parent))
                .map_or(0, |depth| depth + 1);
            depths.insert(step.sequence, depth);
            let title = match &step.kind {
                StepTraceKind::Action {
                    capability,
                    version,
                } => schemas
                    .iter()
                    .find(|schema| schema.id == capability && schema.version == *version)
                    .map_or_else(|| capability.clone(), |schema| schema.title.to_owned()),
                StepTraceKind::SetVariable => "Set variable".into(),
                StepTraceKind::If => "If".into(),
                StepTraceKind::While => "While".into(),
                StepTraceKind::OneOrMore => "One or More".into(),
                StepTraceKind::Delay => "Wait".into(),
                StepTraceKind::RequestInput => "Ask for input".into(),
                StepTraceKind::Call { workflow_id } => format!("Run {workflow_id}"),
                StepTraceKind::Stop => "Stop".into(),
            };
            let (outcome, failure, uncertain, continued) = match &step.outcome {
                StepTraceOutcome::Succeeded => ("Succeeded", String::new(), false, false),
                StepTraceOutcome::Stopped => ("Stopped", String::new(), false, false),
                StepTraceOutcome::Cancelled => ("Cancelled", String::new(), false, false),
                StepTraceOutcome::Failed {
                    kind,
                    remote_effect_uncertain,
                    continued_by_policy,
                } => (
                    "Failed",
                    step_failure_label(kind).to_owned(),
                    *remote_effect_uncertain,
                    *continued_by_policy,
                ),
            };
            HistoryStep {
                sequence: step.sequence.to_string().into(),
                number: step.sequence.saturating_add(1).to_string().into(),
                depth,
                title: title.into(),
                step_id: step.step_id.clone().into(),
                workflow_id: step.workflow_id.clone().into(),
                workflow_revision: step.workflow_revision.to_string().into(),
                started_at: format_step_timestamp(step.started_at_ms).into(),
                duration: format_duration(step.duration_ms).into(),
                outcome: outcome.into(),
                failure: failure.into(),
                uncertain,
                continued,
            }
        })
        .collect()
}

fn step_failure_label(kind: &FailureKind) -> &'static str {
    match kind {
        FailureKind::InvalidDefinition => "The step definition is invalid.",
        FailureKind::MissingValue => "A required value was unavailable.",
        FailureKind::InvalidType => "An input had the wrong type.",
        FailureKind::CapabilityUnavailable => "The action is unavailable.",
        FailureKind::ConnectorUnavailable => "The integration was unavailable.",
        FailureKind::Action => "The action failed.",
        FailureKind::Timeout => "The action timed out.",
        FailureKind::Input => "Input could not be completed.",
        FailureKind::LoopLimit => "The loop reached its iteration limit.",
        FailureKind::StepLimit => "The run reached its step limit.",
        FailureKind::CallDepth => "The run reached its workflow call limit.",
        FailureKind::CalledWorkflow => "The called workflow failed.",
        FailureKind::NoChildSucceeded => "No action in this group succeeded.",
    }
}

fn outcome_label(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Running => "Running".to_owned(),
        RunOutcome::Succeeded => "Succeeded".to_owned(),
        RunOutcome::Stopped => "Stopped".to_owned(),
        RunOutcome::Cancelled => "Cancelled".to_owned(),
        RunOutcome::Failed { category } => {
            format!("Failed · {}", failure_category_label(*category))
        }
        RunOutcome::Rejected => "Rejected".to_owned(),
        RunOutcome::Interrupted => "Interrupted".to_owned(),
    }
}

fn failure_category_label(category: crate::history::FailureCategory) -> &'static str {
    use crate::history::FailureCategory;

    match category {
        FailureCategory::Definition => "definition",
        FailureCategory::MissingValue => "missing value",
        FailureCategory::InvalidType => "invalid type",
        FailureCategory::Capability => "capability",
        FailureCategory::Connector => "connector",
        FailureCategory::Action => "action",
        FailureCategory::Timeout => "timeout",
        FailureCategory::Input => "input",
        FailureCategory::Limit => "limit",
        FailureCategory::Other => "other",
    }
}

fn trigger_label(kind: TriggerKind, id: &str) -> String {
    let kind = match kind {
        TriggerKind::Manual => "Manual",
        TriggerKind::Schedule => "Schedule",
        TriggerKind::Event => "Event",
        TriggerKind::Webhook => "Webhook",
    };
    if id.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind} · {id}")
    }
}

fn format_duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds} ms")
    } else if milliseconds < 60_000 {
        format!("{:.1} s", milliseconds as f64 / 1_000.0)
    } else {
        let seconds = milliseconds / 1_000;
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn format_timestamp(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    let days = (seconds / 86_400) as i64;
    let time_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60
    )
}

fn format_step_timestamp(milliseconds: u64) -> String {
    let minute = format_timestamp(milliseconds);
    format!(
        "{}:{:02}.{:03} UTC",
        minute.trim_end_matches(" UTC"),
        (milliseconds / 1_000) % 60,
        milliseconds % 1_000
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let shifted_days = days_since_epoch + 719_468;
    let era = shifted_days.div_euclid(146_097);
    let day_of_era = shifted_days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::engine::{ScriptActivity, ScriptOutcome, Workflow};
    use crate::history::{RunHistory, RunOutcome, TriggerIdentity, TriggerKind};
    use crate::workflows::{WorkflowDefinition, WorkflowRepository};

    use super::{
        format_duration, format_step_timestamp, format_timestamp, history_rows, load_history,
        trace_rows,
    };

    #[test]
    fn history_rows_use_saved_titles_preserve_corrupt_records_and_show_script_results() {
        let directory = tempfile::tempdir().unwrap();
        let history = RunHistory::new(directory.path().join("history"));
        let repository = WorkflowRepository::at(directory.path().join("workflows"));
        let mut definition = WorkflowDefinition::manual(Workflow {
            id: "daily".to_owned(),
            revision: 4,
            overlap: false,
            steps: Vec::new(),
            outputs: BTreeMap::new(),
        });
        definition.name = Some("A lovely stream".to_owned());
        repository.create_definition(&definition).unwrap();

        let running = history
            .begin(
                "daily",
                4,
                TriggerIdentity {
                    kind: TriggerKind::Event,
                    id: Some("recording-started".to_owned()),
                },
            )
            .unwrap();
        history
            .finish_with_activity(
                &running.run_id,
                RunOutcome::Failed {
                    category: crate::history::FailureCategory::Action,
                },
                vec![ScriptActivity {
                    workflow_id: "daily".to_owned(),
                    step_id: "script-1".to_owned(),
                    outcome: ScriptOutcome::Failed,
                }],
            )
            .unwrap();
        std::fs::write(
            history_dir(directory.path()).join("broken.json"),
            b"{broken",
        )
        .unwrap();

        let rows = history_rows(load_history(&history, &repository).unwrap(), &[]);
        let valid = rows
            .iter()
            .find(|row| row.run_id == running.run_id)
            .unwrap();
        assert_eq!(valid.title, "A lovely stream");
        assert_eq!(valid.outcome, "Failed · action");
        assert_eq!(valid.trigger, "Event · recording-started");
        assert_eq!(valid.workflow_revision, "4");
        assert!(valid.script_activity.contains("daily · script-1: Failed"));

        let corrupt = rows.iter().find(|row| row.is_corrupt).unwrap();
        assert_eq!(corrupt.run_id, "broken.json");
        assert_eq!(corrupt.outcome, "Corrupt JSON");
        assert!(!corrupt.error.is_empty());
        assert_eq!(
            std::fs::read(history_dir(directory.path()).join("broken.json")).unwrap(),
            b"{broken"
        );
    }

    #[test]
    fn refreshing_history_does_not_interrupt_a_live_run() {
        let directory = tempfile::tempdir().unwrap();
        let history = RunHistory::new(directory.path().join("history"));
        let repository = WorkflowRepository::at(directory.path().join("workflows"));
        let running = history
            .begin(
                "active",
                1,
                TriggerIdentity {
                    kind: TriggerKind::Manual,
                    id: None,
                },
            )
            .unwrap();

        let rows = history_rows(load_history(&history, &repository).unwrap(), &[]);
        assert!(rows.iter().any(|row| row.outcome == "Running"));
        assert_eq!(
            history.load(&running.run_id).unwrap().outcome,
            RunOutcome::Running
        );
    }

    #[test]
    fn trace_rows_show_nested_outcomes_and_unknown_action_identity() {
        use crate::engine::{FailureKind, StepTrace, StepTraceKind, StepTraceOutcome};
        let group = StepTrace {
            sequence: 1,
            parent_sequence: None,
            workflow_id: "sample".into(),
            workflow_revision: 7,
            step_id: "group".into(),
            kind: StepTraceKind::OneOrMore,
            started_at_ms: 1_234,
            finished_at_ms: 2_234,
            duration_ms: 1_000,
            outcome: StepTraceOutcome::Succeeded,
        };
        let child = StepTrace {
            sequence: 2,
            parent_sequence: Some(1),
            step_id: "send".into(),
            kind: StepTraceKind::Action {
                capability: "missing.send".into(),
                version: 3,
            },
            outcome: StepTraceOutcome::Failed {
                kind: FailureKind::Timeout,
                remote_effect_uncertain: true,
                continued_by_policy: false,
            },
            ..group.clone()
        };
        let rows = trace_rows(&[group, child], &[]);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].title, "missing.send");
        assert_eq!(rows[1].workflow_revision, "7");
        assert_eq!(rows[1].started_at, "1970-01-01 00:00:01.234 UTC");
        assert!(rows[1].uncertain);
        assert!(!rows[1].continued);
        assert_eq!(rows[0].outcome, "Succeeded");
        assert_eq!(rows[1].outcome, "Failed");
        assert_eq!(format_step_timestamp(60_000), "1970-01-01 00:01:00.000 UTC");
    }

    #[test]
    fn timestamps_and_durations_are_readable() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_duration(950), "950 ms");
        assert_eq!(format_duration(61_000), "1m 1s");
    }

    fn history_dir(root: &std::path::Path) -> std::path::PathBuf {
        root.join("history")
    }
}
