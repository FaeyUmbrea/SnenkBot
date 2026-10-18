use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;

use serde_json::Value;
use thiserror::Error;

use crate::migration::StoredConfig;

const BACKUP_LIMIT: usize = 10;
#[derive(Clone, Debug)]
pub struct ConfigSnapshot {
    config: StoredConfig,
    original_bytes: Vec<u8>,
    envelope: Value,
    id: String,
}

#[derive(Clone, Debug)]
pub struct FileRevision {
    bytes: Vec<u8>,
    id: String,
}

impl FileRevision {
    pub fn original_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl ConfigSnapshot {
    pub fn config(&self) -> &StoredConfig {
        &self.config
    }

    pub fn original_bytes(&self) -> &[u8] {
        &self.original_bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupInfo {
    pub file_name: String,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveWarning {
    DirectorySync(String),
    BackupRetention(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveOutcome {
    /// False means the document already contained the requested value.
    pub published: bool,
    pub warnings: Vec<SaveWarning>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("invalid document ID `{0}`")]
    InvalidId(String),
    #[error("document `{0}` does not exist")]
    NotFound(String),
    #[error("document `{0}` already exists")]
    AlreadyExists(String),
    #[error("document `{0}` changed since it was loaded")]
    Conflict(String),
    #[error("document `{id}` is not a JSON object: {source}")]
    InvalidDocument {
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("document `{0}` has an invalid envelope")]
    InvalidEnvelope(String),
    #[error("validation failed for `{id}`: {message}")]
    Validation { id: String, message: String },
    #[error("I/O error while handling `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("backup sequence for `{0}` is exhausted")]
    BackupSequenceExhausted(String),
}

/// Stores one JSON document per ID and retains the ten most recent prior files.
/// Callers must validate definition/version compatibility and serialize credentials
/// separately; this store does not identify or remove secret fields.
///
/// The lock is advisory and coordinates processes that use this same store API.
/// Windows replacement is same-volume via a sibling temporary file. Windows does
/// not expose directory syncing through this implementation, so rename durability
/// across sudden power loss cannot be promised there.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    root: PathBuf,
    #[cfg(test)]
    failpoint: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl ConfigStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            #[cfg(test)]
            failpoint: std::sync::Arc::default(),
        }
    }

    pub fn load(&self, id: &str) -> Result<ConfigSnapshot, StoreError> {
        let path = self.document_path(id)?;
        self.with_lock(|| {
            let original_bytes = read(&path)?.ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
            let envelope: Value = serde_json::from_slice(&original_bytes).map_err(|source| {
                StoreError::InvalidDocument {
                    id: id.to_owned(),
                    source,
                }
            })?;
            let config = parse_config(id, &envelope)?;
            Ok(ConfigSnapshot {
                config,
                original_bytes,
                envelope,
                id: id.to_owned(),
            })
        })
    }

    /// Captures exact current bytes even when the file cannot be parsed.
    pub fn observe(&self, id: &str) -> Result<FileRevision, StoreError> {
        let path = self.document_path(id)?;
        self.with_lock(|| {
            let bytes = read(&path)?.ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
            Ok(FileRevision {
                bytes,
                id: id.to_owned(),
            })
        })
    }

    pub fn create<F>(
        &self,
        id: &str,
        config: &StoredConfig,
        validate: F,
    ) -> Result<SaveOutcome, StoreError>
    where
        F: FnOnce(&StoredConfig) -> Result<(), String>,
    {
        validate(config).map_err(|message| StoreError::Validation {
            id: id.to_owned(),
            message,
        })?;
        let path = self.document_path(id)?;
        self.with_lock(|| {
            if path.exists() {
                return Err(StoreError::AlreadyExists(id.to_owned()));
            }
            self.publish(id, &path, &make_envelope(config)?, None)
        })
    }

    pub fn save<F>(
        &self,
        id: &str,
        snapshot: &ConfigSnapshot,
        config: &StoredConfig,
        validate: F,
    ) -> Result<SaveOutcome, StoreError>
    where
        F: FnOnce(&StoredConfig) -> Result<(), String>,
    {
        validate(config).map_err(|message| StoreError::Validation {
            id: id.to_owned(),
            message,
        })?;
        let path = self.document_path(id)?;
        if snapshot.id != id {
            return Err(StoreError::InvalidId(id.to_owned()));
        }
        self.with_lock(|| {
            let current = read(&path)?.ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
            if current != snapshot.original_bytes {
                return Err(StoreError::Conflict(id.to_owned()));
            }
            let mut envelope = snapshot.envelope.clone();
            update_envelope(&mut envelope, config)?;
            if envelope == snapshot.envelope {
                return Ok(SaveOutcome {
                    published: false,
                    warnings: Vec::new(),
                });
            }
            self.publish(id, &path, &envelope, Some(&current))
        })
    }

    /// Explicitly restores a selected backup, using the current snapshot as the
    /// concurrency check and preserving any fields stored in the backup envelope.
    pub fn restore<F>(
        &self,
        id: &str,
        current: &FileRevision,
        backup: &ConfigSnapshot,
        validate: F,
    ) -> Result<SaveOutcome, StoreError>
    where
        F: FnOnce(&StoredConfig) -> Result<(), String>,
    {
        validate(&backup.config).map_err(|message| StoreError::Validation {
            id: id.to_owned(),
            message,
        })?;
        let path = self.document_path(id)?;
        if current.id != id || backup.id != id {
            return Err(StoreError::InvalidId(id.to_owned()));
        }
        self.with_lock(|| {
            let bytes = read(&path)?.ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
            if bytes != current.bytes {
                return Err(StoreError::Conflict(id.to_owned()));
            }
            if serde_json::from_slice::<Value>(&bytes).ok().as_ref() == Some(&backup.envelope) {
                return Ok(SaveOutcome {
                    published: false,
                    warnings: Vec::new(),
                });
            }
            self.publish(id, &path, &backup.envelope, Some(&bytes))
        })
    }

    pub fn list_backups(&self, id: &str) -> Result<Vec<BackupInfo>, StoreError> {
        self.document_path(id)?;
        self.with_lock(|| self.backups(id))
    }

    pub fn read_backup(&self, id: &str, file_name: &str) -> Result<ConfigSnapshot, StoreError> {
        self.document_path(id)?;
        if backup_sequence(id, file_name).is_none() {
            return Err(StoreError::InvalidId(file_name.to_owned()));
        }
        let path = self.root.join("history").join(file_name);
        self.with_lock(|| {
            let original_bytes =
                read(&path)?.ok_or_else(|| StoreError::NotFound(file_name.into()))?;
            let envelope: Value = serde_json::from_slice(&original_bytes).map_err(|source| {
                StoreError::InvalidDocument {
                    id: id.to_owned(),
                    source,
                }
            })?;
            let config = parse_config(id, &envelope)?;
            Ok(ConfigSnapshot {
                config,
                original_bytes,
                envelope,
                id: id.to_owned(),
            })
        })
    }

    fn document_path(&self, id: &str) -> Result<PathBuf, StoreError> {
        validate_id(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        fs::create_dir_all(&self.root).map_err(|source| io_error(&self.root, source))?;
        let lock_path = self.root.join(".store.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| io_error(&lock_path, source))?;
        lock.lock().map_err(|source| io_error(&lock_path, source))?;
        let result = operation();
        drop(lock);
        result
    }

    fn publish(
        &self,
        id: &str,
        path: &Path,
        envelope: &Value,
        previous: Option<&[u8]>,
    ) -> Result<SaveOutcome, StoreError> {
        fs::create_dir_all(&self.root).map_err(|source| io_error(&self.root, source))?;
        let history = self.root.join("history");
        if previous.is_some() {
            fs::create_dir_all(&history).map_err(|source| io_error(&history, source))?;
        }
        let bytes = serde_json::to_vec_pretty(envelope)?;
        #[cfg(test)]
        self.fail_if(FailPoint::TemporaryWrite, &self.root)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)
            .map_err(|source| io_error(&self.root, source))?;
        temporary
            .write_all(&bytes)
            .map_err(|source| io_error(temporary.path(), source))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|source| io_error(temporary.path(), source))?;
        #[cfg(test)]
        self.fail_if(FailPoint::TemporarySync, &self.root)?;

        if let Some(previous) = previous {
            let backups = self.backups(id)?;
            let latest_matches = if let Some(latest) = backups.last() {
                let latest_path = history.join(&latest.file_name);
                read(&latest_path)?.as_deref() == Some(previous)
            } else {
                false
            };
            if !latest_matches {
                let sequence = match backups.last() {
                    Some(backup) => backup
                        .sequence
                        .checked_add(1)
                        .ok_or_else(|| StoreError::BackupSequenceExhausted(id.to_owned()))?,
                    None => 1,
                };
                let backup_path = history.join(backup_name(id, sequence));
                write_backup(&backup_path, previous)?;
            }
            // A retry may reuse a backup whose directory sync previously failed.
            #[cfg(test)]
            self.fail_if(FailPoint::BackupSync, &history)?;
            sync_directory(&history).map_err(|source| io_error(&history, source))?;
            sync_directory(&self.root).map_err(|source| io_error(&self.root, source))?;
        }

        #[cfg(test)]
        self.fail_if(FailPoint::Publish, path)?;
        temporary
            .persist(path)
            .map_err(|error| io_error(path, error.error))?;

        let mut warnings = Vec::new();
        #[cfg(unix)]
        if let Err(error) = File::open(&self.root).and_then(|directory| directory.sync_all()) {
            warnings.push(SaveWarning::DirectorySync(error.to_string()));
        }

        if previous.is_some() {
            #[cfg(test)]
            if self.take_failpoint(FailPoint::Prune) {
                warnings.push(SaveWarning::BackupRetention("injected failure".into()));
                return Ok(SaveOutcome {
                    published: true,
                    warnings,
                });
            }
            match self.prune_backups(id) {
                Ok(()) => {}
                Err(error) => warnings.push(SaveWarning::BackupRetention(error.to_string())),
            }
        }

        Ok(SaveOutcome {
            published: true,
            warnings,
        })
    }

    fn backups(&self, id: &str) -> Result<Vec<BackupInfo>, StoreError> {
        let history = self.root.join("history");
        let entries = match fs::read_dir(&history) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(io_error(&history, source)),
        };
        let mut backups = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&history, source))?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(sequence) = backup_sequence(id, &file_name) else {
                continue;
            };
            backups.push(BackupInfo {
                file_name,
                sequence,
            });
        }
        backups.sort_by_key(|backup| backup.sequence);
        Ok(backups)
    }

    fn prune_backups(&self, id: &str) -> Result<(), StoreError> {
        let backups = self.backups(id)?;
        for backup in backups
            .iter()
            .take(backups.len().saturating_sub(BACKUP_LIMIT))
        {
            let path = self.root.join("history").join(&backup.file_name);
            fs::remove_file(&path).map_err(|source| io_error(path, source))?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn set_failpoint(&self, failpoint: FailPoint) {
        self.failpoint.store(failpoint as u8, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn take_failpoint(&self, failpoint: FailPoint) -> bool {
        self.failpoint
            .compare_exchange(
                failpoint as u8,
                FailPoint::None as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    #[cfg(test)]
    fn fail_if(&self, failpoint: FailPoint, path: &Path) -> Result<(), StoreError> {
        if self.take_failpoint(failpoint) {
            return Err(io_error(
                path,
                std::io::Error::other("injected storage failure"),
            ));
        }
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<(), StoreError> {
    let path = Path::new(id);
    let valid_chars = id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'));
    let stem = id.split('.').next().unwrap_or_default();
    let reserved = ["con", "prn", "aux", "nul"]
        .iter()
        .any(|name| stem.eq_ignore_ascii_case(name))
        || ["com", "lpt"].iter().any(|prefix| {
            stem.get(..3)
                .is_some_and(|stem_prefix| stem_prefix.eq_ignore_ascii_case(prefix))
                && stem
                    .get(3..)
                    .and_then(|digit| digit.parse::<u8>().ok())
                    .is_some_and(|digit| (1..=9).contains(&digit))
        });
    if id.is_empty()
        || id == "."
        || id == ".."
        || !valid_chars
        || reserved
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || id.ends_with('.')
    {
        return Err(StoreError::InvalidId(id.to_owned()));
    }
    Ok(())
}

fn parse_config(id: &str, envelope: &Value) -> Result<StoredConfig, StoreError> {
    if !envelope.is_object() {
        return Err(StoreError::InvalidEnvelope(id.to_owned()));
    }
    serde_json::from_value(envelope.clone()).map_err(|source| StoreError::InvalidDocument {
        id: id.to_owned(),
        source,
    })
}

fn make_envelope(config: &StoredConfig) -> Result<Value, StoreError> {
    Ok(serde_json::to_value(config)?)
}

fn update_envelope(envelope: &mut Value, config: &StoredConfig) -> Result<(), StoreError> {
    let object = envelope
        .as_object_mut()
        .ok_or_else(|| StoreError::InvalidEnvelope("snapshot".to_owned()))?;
    let updated = make_envelope(config)?;
    let updated = updated
        .as_object()
        .ok_or_else(|| StoreError::InvalidEnvelope("new config".to_owned()))?;
    for (key, value) in updated {
        object.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn write_backup(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().expect("backup path has a parent");
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| io_error(parent, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error(file.path(), source))?;
    file.as_file()
        .sync_all()
        .map_err(|source| io_error(file.path(), source))?;
    file.persist_noclobber(path)
        .map_err(|error| io_error(path, error.error))?;
    Ok(())
}

fn backup_name(id: &str, sequence: u64) -> String {
    format!("{id}.{sequence:020}.json")
}

fn backup_sequence(id: &str, file_name: &str) -> Option<u64> {
    let prefix = format!("{id}.");
    let sequence = file_name.strip_prefix(&prefix)?.strip_suffix(".json")?;
    if sequence.len() != 20 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    sequence.parse().ok()
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
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

fn read(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    Ok(Some(bytes))
}

fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> StoreError {
    StoreError::Io {
        path: path.into(),
        source,
    }
}

#[cfg(test)]
#[repr(u8)]
#[derive(Clone, Copy)]
enum FailPoint {
    None,
    TemporaryWrite,
    TemporarySync,
    BackupSync,
    Publish,
    Prune,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn config(version: u32, value: &str) -> StoredConfig {
        StoredConfig {
            definition: "example.settings".to_owned(),
            version,
            data: json!({"value": value}),
        }
    }

    fn accept(_: &StoredConfig) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn creates_replaces_and_reads_exact_prior_backup() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let original = store.load("settings").unwrap();
        let original_bytes = original.original_bytes().to_vec();

        let outcome = store
            .save("settings", &original, &config(2, "new"), accept)
            .unwrap();

        assert!(outcome.published);
        assert!(outcome.warnings.is_empty());
        assert_eq!(store.load("settings").unwrap().config(), &config(2, "new"));
        let backups = store.list_backups("settings").unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            store
                .read_backup("settings", &backups[0].file_name)
                .unwrap()
                .original_bytes(),
            original_bytes
        );
    }

    #[test]
    fn preserves_unrecognized_envelope_fields_on_save() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let path = directory.path().join("settings.json");
        let mut envelope: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        envelope
            .as_object_mut()
            .unwrap()
            .insert("future_metadata".into(), json!({"kept": true}));
        fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        let snapshot = store.load("settings").unwrap();

        store
            .save("settings", &snapshot, &config(2, "new"), accept)
            .unwrap();

        let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["future_metadata"], json!({"kept": true}));
    }

    #[test]
    fn rejects_malformed_json_without_changing_bytes() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        let path = directory.path().join("settings.json");
        let malformed = b"{ broken";
        fs::write(&path, malformed).unwrap();

        assert!(matches!(
            store.load("settings"),
            Err(StoreError::InvalidDocument { .. })
        ));
        assert_eq!(fs::read(path).unwrap(), malformed);
    }

    #[test]
    fn can_explicitly_restore_a_backup_over_malformed_current_file() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let first = store.load("settings").unwrap();
        store
            .save("settings", &first, &config(2, "new"), accept)
            .unwrap();
        let backup = store
            .read_backup(
                "settings",
                &store.list_backups("settings").unwrap()[0].file_name,
            )
            .unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"{ broken").unwrap();
        let corrupt_revision = store.observe("settings").unwrap();

        store
            .restore("settings", &corrupt_revision, &backup, accept)
            .unwrap();

        assert_eq!(store.load("settings").unwrap().config(), &config(1, "old"));
    }

    #[test]
    fn validation_failure_writes_nothing() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let snapshot = store.load("settings").unwrap();
        let before = snapshot.original_bytes().to_vec();
        let result = store.save("settings", &snapshot, &config(2, "bad"), |_| {
            Err("invalid definition".to_owned())
        });

        assert!(matches!(result, Err(StoreError::Validation { .. })));
        assert_eq!(store.load("settings").unwrap().original_bytes(), before);
        assert!(store.list_backups("settings").unwrap().is_empty());
    }

    #[test]
    fn backup_failure_stops_before_canonical_publication() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let snapshot = store.load("settings").unwrap();
        fs::write(directory.path().join("history"), b"occupied").unwrap();

        assert!(matches!(
            store.save("settings", &snapshot, &config(2, "new"), accept),
            Err(StoreError::Io { .. })
        ));
        assert_eq!(store.load("settings").unwrap().config(), &config(1, "old"));
    }

    #[test]
    fn one_document_can_fail_while_another_is_saved() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("first", &config(1, "first"), accept).unwrap();
        store
            .create("second", &config(1, "second"), accept)
            .unwrap();
        let first = store.load("first").unwrap();
        let second = store.load("second").unwrap();

        assert!(
            store
                .save("first", &first, &config(2, "invalid"), |_| Err(
                    "rejected".into()
                ))
                .is_err()
        );
        store
            .save("second", &second, &config(2, "saved"), accept)
            .unwrap();

        assert_eq!(store.load("first").unwrap().config(), &config(1, "first"));
        assert_eq!(store.load("second").unwrap().config(), &config(2, "saved"));
    }

    #[test]
    fn stale_snapshot_conflicts_without_overwriting_newer_save() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let stale = store.load("settings").unwrap();
        let current = store.load("settings").unwrap();
        store
            .save("settings", &current, &config(2, "winner"), accept)
            .unwrap();

        assert!(matches!(
            store.save("settings", &stale, &config(2, "loser"), accept),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            store.load("settings").unwrap().config(),
            &config(2, "winner")
        );
    }

    #[test]
    fn simultaneous_writers_with_same_snapshot_have_one_winner() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let snapshot = Arc::new(store.load("settings").unwrap());
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|value| {
                let store = store.clone();
                let snapshot = Arc::clone(&snapshot);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store.save("settings", &snapshot, &config(2, value), accept)
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(StoreError::Conflict(_))))
                .count(),
            1
        );
    }

    #[test]
    fn retains_ten_previous_files() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(0, "0"), accept).unwrap();
        for version in 1..=12 {
            let snapshot = store.load("settings").unwrap();
            store
                .save(
                    "settings",
                    &snapshot,
                    &config(version, &version.to_string()),
                    accept,
                )
                .unwrap();
        }
        assert_eq!(store.list_backups("settings").unwrap().len(), BACKUP_LIMIT);
    }

    #[test]
    fn repeated_failed_publications_do_not_displace_saved_versions() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(0, "0"), accept).unwrap();
        for version in 1..=10 {
            let snapshot = store.load("settings").unwrap();
            store
                .save(
                    "settings",
                    &snapshot,
                    &config(version, &version.to_string()),
                    accept,
                )
                .unwrap();
        }

        let snapshot = store.load("settings").unwrap();
        for _ in 0..3 {
            store.set_failpoint(FailPoint::Publish);
            assert!(
                store
                    .save("settings", &snapshot, &config(11, "11"), accept)
                    .is_err()
            );
        }
        store
            .save("settings", &snapshot, &config(11, "11"), accept)
            .unwrap();

        let versions: Vec<_> = store
            .list_backups("settings")
            .unwrap()
            .iter()
            .map(|backup| {
                store
                    .read_backup("settings", &backup.file_name)
                    .unwrap()
                    .config()
                    .version
            })
            .collect();
        assert_eq!(versions, (1..=10).collect::<Vec<_>>());
    }

    #[test]
    fn no_op_save_does_not_create_a_backup() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store
            .create("settings", &config(1, "same"), accept)
            .unwrap();
        let snapshot = store.load("settings").unwrap();

        let outcome = store
            .save("settings", &snapshot, &config(1, "same"), accept)
            .unwrap();

        assert!(!outcome.published);
        assert!(store.list_backups("settings").unwrap().is_empty());
    }

    #[test]
    fn reused_backup_must_finish_syncing_before_publication() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let snapshot = store.load("settings").unwrap();

        for _ in 0..2 {
            store.set_failpoint(FailPoint::BackupSync);
            assert!(
                store
                    .save("settings", &snapshot, &config(2, "new"), accept)
                    .is_err()
            );
            assert_eq!(
                store.observe("settings").unwrap().original_bytes(),
                snapshot.original_bytes()
            );
            assert_eq!(store.list_backups("settings").unwrap().len(), 1);
        }

        store
            .save("settings", &snapshot, &config(2, "new"), accept)
            .unwrap();
        assert_eq!(store.load("settings").unwrap().config(), &config(2, "new"));
        assert_eq!(store.list_backups("settings").unwrap().len(), 1);
    }

    #[test]
    fn rejects_unsafe_ids() {
        let store = ConfigStore::new("unused");
        for id in [
            "", ".", "..", "../other", "a/b", "CON", "con.json", "COM1.log", "LPT9", "bad name",
        ] {
            assert!(matches!(store.load(id), Err(StoreError::InvalidId(_))));
        }
    }

    #[test]
    fn backup_reader_rejects_path_traversal() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        assert!(matches!(
            store.read_backup("settings", "settings.foo/../../outside.json"),
            Err(StoreError::InvalidId(_))
        ));
    }

    #[test]
    fn prepublication_failures_leave_original_file_unchanged() {
        for failpoint in [
            FailPoint::TemporaryWrite,
            FailPoint::TemporarySync,
            FailPoint::Publish,
        ] {
            let directory = tempdir().unwrap();
            let store = ConfigStore::new(directory.path());
            store.create("settings", &config(1, "old"), accept).unwrap();
            let snapshot = store.load("settings").unwrap();
            let before = snapshot.original_bytes().to_vec();
            store.set_failpoint(failpoint);

            assert!(
                store
                    .save("settings", &snapshot, &config(2, "new"), accept)
                    .is_err()
            );
            assert_eq!(store.observe("settings").unwrap().original_bytes(), before);
        }
    }

    #[test]
    fn reports_retention_failure_after_successful_publication() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let snapshot = store.load("settings").unwrap();
        store.set_failpoint(FailPoint::Prune);

        let outcome = store
            .save("settings", &snapshot, &config(2, "new"), accept)
            .unwrap();

        assert!(outcome.published);
        assert_eq!(
            outcome.warnings,
            [SaveWarning::BackupRetention("injected failure".into())]
        );
        assert_eq!(store.load("settings").unwrap().config(), &config(2, "new"));
    }

    #[test]
    fn migrated_config_can_be_saved_and_reopened() {
        use serde::{Deserialize, Serialize};

        use crate::ConfigSchema;
        use crate::migration::load_config;
        use crate::schema::DescribeConfig;

        #[derive(Default, Deserialize, Serialize, ConfigSchema)]
        #[config(id = "example.settings", version = 2, title = "Settings")]
        struct Settings {
            #[config(id = "value", label = "Value", introduced = 1)]
            value: String,
            #[config(id = "enabled", label = "Enabled", introduced = 2)]
            enabled: bool,
        }

        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let snapshot = store.load("settings").unwrap();
        let loaded = load_config::<Settings>(&snapshot.config, &[]).unwrap();
        assert!(loaded.migrated);

        store
            .save("settings", &snapshot, &loaded.stored, |stored| {
                if stored.definition == Settings::SCHEMA.id
                    && stored.version == Settings::SCHEMA.version
                {
                    Ok(())
                } else {
                    Err("wrong schema".into())
                }
            })
            .unwrap();

        let reopened = store.load("settings").unwrap();
        let loaded_again = load_config::<Settings>(&reopened.config, &[]).unwrap();
        assert!(!loaded_again.migrated);
        assert_eq!(loaded_again.value.value, "old");
        assert!(!loaded_again.value.enabled);
    }

    #[test]
    fn explicit_restore_publishes_selected_backup() {
        let directory = tempdir().unwrap();
        let store = ConfigStore::new(directory.path());
        store.create("settings", &config(1, "old"), accept).unwrap();
        let prior = store.load("settings").unwrap();
        store
            .save("settings", &prior, &config(2, "new"), accept)
            .unwrap();
        let backup = store
            .read_backup(
                "settings",
                &store.list_backups("settings").unwrap()[0].file_name,
            )
            .unwrap();
        let current = store.observe("settings").unwrap();

        store
            .restore("settings", &current, &backup, accept)
            .unwrap();

        assert_eq!(store.load("settings").unwrap().config(), &config(1, "old"));
    }
}
