//! Native app directories and staged import of the previous desktop's local data.

use std::fs::{self, OpenOptions};
use std::path::Path;

use snenk_bot::paths::AppPaths;
use tauri::{Manager, Runtime};

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("could not resolve application directories: {0}")]
    Tauri(#[from] tauri::Error),
    #[error("could not resolve previous application directories: {0}")]
    Legacy(String),
    #[error("could not import previous application data: {0}")]
    Io(#[from] std::io::Error),
    #[error("another application is importing previous data")]
    Busy,
    #[error("application data contains a symbolic link or unsupported file")]
    UnsupportedFile,
    #[error("previous and current application data directories overlap")]
    OverlappingPaths,
}

pub fn resolve<R: Runtime>(app: &tauri::AppHandle<R>) -> Result<AppPaths, DirectoryError> {
    let paths = app.path();
    Ok(AppPaths {
        config: paths.app_config_dir()?,
        data: paths.app_data_dir()?,
        state: paths.app_local_data_dir()?,
    })
}

/// Imports known data trees without merging or replacing an existing destination.
/// Each tree is published by rename only after a complete, synced copy. Retaining
/// the source keeps the preserved stream executable usable during desktop cutover.
pub fn migrate_legacy(legacy: &AppPaths, current: &AppPaths) -> Result<(), DirectoryError> {
    let trees = [
        (legacy.integrations_dir(), current.integrations_dir()),
        (legacy.workflows_dir(), current.workflows_dir()),
        (legacy.history_dir(), current.history_dir()),
    ];
    let mut has_source = false;
    for (source, target) in &trees {
        if source == target {
            continue;
        }
        if target.starts_with(source) || source.starts_with(target) {
            return Err(DirectoryError::OverlappingPaths);
        }
        match fs::symlink_metadata(source) {
            Ok(_) => has_source = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !has_source {
        return Ok(());
    }
    fs::create_dir_all(&current.config)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(current.config.join(".legacy-import.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(DirectoryError::Busy),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    for (source, target) in trees {
        if source == target {
            continue;
        }
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(_) => return Err(DirectoryError::UnsupportedFile),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(DirectoryError::UnsupportedFile),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        let parent = target.parent().expect("application tree has a parent");
        fs::create_dir_all(parent)?;
        let staging = tempfile::Builder::new()
            .prefix(".legacy-import-")
            .tempdir_in(parent)?;
        let copied = staging.path().join("data");
        copy_tree(&source, &copied)?;
        fs::rename(&copied, &target)?;
        sync_directory(parent)?;
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), DirectoryError> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            let mut source = fs::File::open(entry.path())?;
            let mut copied = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)?;
            std::io::copy(&mut source, &mut copied)?;
            copied.set_permissions(source.metadata()?.permissions())?;
            copied.sync_all()?;
        } else {
            return Err(DirectoryError::UnsupportedFile);
        }
    }
    sync_directory(target)?;
    Ok(())
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> AppPaths {
        AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn imports_all_trees_and_preserves_originals_backups_and_unknown_fields() {
        let fixture = tempfile::tempdir().unwrap();
        let old = paths(&fixture.path().join("old"));
        let new = paths(&fixture.path().join("new"));
        let files = [
            (
                old.integrations_dir(),
                new.integrations_dir(),
                "twitch/settings.json",
            ),
            (old.workflows_dir(), new.workflows_dir(), "details.json"),
            (
                old.workflows_dir(),
                new.workflows_dir(),
                "backups/details.json",
            ),
            (old.history_dir(), new.history_dir(), "run.json"),
        ];
        let bytes = br#"{"future":{"field":42},"revision":7}"#;
        for (source, _, file) in &files {
            write(&source.join(file), bytes);
        }
        migrate_legacy(&old, &new).unwrap();
        for (source, target, file) in &files {
            assert_eq!(fs::read(source.join(file)).unwrap(), bytes);
            assert_eq!(fs::read(target.join(file)).unwrap(), bytes);
        }
        write(&new.workflows_dir().join("details.json"), b"edited");
        migrate_legacy(&old, &new).unwrap();
        assert_eq!(
            fs::read(new.workflows_dir().join("details.json")).unwrap(),
            b"edited"
        );
    }

    #[test]
    fn imports_read_only_files_without_changing_source_permissions() {
        let fixture = tempfile::tempdir().unwrap();
        let old = paths(&fixture.path().join("old"));
        let new = paths(&fixture.path().join("new"));
        let source = old.history_dir().join("run.json");
        write(&source, b"history");
        let mut permissions = fs::metadata(&source).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&source, permissions).unwrap();
        migrate_legacy(&old, &new).unwrap();
        assert_eq!(
            fs::read(new.history_dir().join("run.json")).unwrap(),
            b"history"
        );
        assert!(fs::metadata(&source).unwrap().permissions().readonly());
        assert!(
            fs::metadata(new.history_dir().join("run.json"))
                .unwrap()
                .permissions()
                .readonly()
        );
    }

    #[test]
    fn resumes_a_partial_import_without_merging_existing_new_data() {
        let fixture = tempfile::tempdir().unwrap();
        let old = paths(&fixture.path().join("old"));
        let new = paths(&fixture.path().join("new"));
        write(&old.integrations_dir().join("settings.json"), b"old");
        write(&new.integrations_dir().join("settings.json"), b"new");
        write(&old.workflows_dir().join("flow.json"), b"workflow");
        migrate_legacy(&old, &new).unwrap();
        assert_eq!(
            fs::read(new.integrations_dir().join("settings.json")).unwrap(),
            b"new"
        );
        assert_eq!(
            fs::read(new.workflows_dir().join("flow.json")).unwrap(),
            b"workflow"
        );
    }

    #[test]
    fn missing_or_identical_source_creates_no_migration_files() {
        let fixture = tempfile::tempdir().unwrap();
        let old = paths(&fixture.path().join("old"));
        let new = paths(&fixture.path().join("new"));
        migrate_legacy(&old, &new).unwrap();
        assert!(!new.config.exists());
        write(&old.workflows_dir().join("flow.json"), b"workflow");
        migrate_legacy(&old, &old).unwrap();
        assert!(!old.config.exists());
    }

    #[test]
    fn overlapping_roots_fail_before_creating_destination_files() {
        let fixture = tempfile::tempdir().unwrap();
        let old = paths(fixture.path());
        write(&old.workflows_dir().join("flow.json"), b"workflow");
        let new = AppPaths {
            config: old.config.join("nested"),
            data: old.workflows_dir(),
            state: old.state.join("nested"),
        };
        assert!(matches!(
            migrate_legacy(&old, &new),
            Err(DirectoryError::OverlappingPaths)
        ));
        assert!(!new.config.exists());
    }

    #[test]
    fn concurrent_import_fails_before_publishing_data() {
        let fixture = tempfile::tempdir().unwrap();
        let old = paths(&fixture.path().join("old"));
        let new = paths(&fixture.path().join("new"));
        write(&old.workflows_dir().join("flow.json"), b"workflow");
        fs::create_dir_all(&new.config).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(new.config.join(".legacy-import.lock"))
            .unwrap();
        lock.lock().unwrap();
        assert!(matches!(
            migrate_legacy(&old, &new),
            Err(DirectoryError::Busy)
        ));
        assert!(!new.workflows_dir().exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejected_symlink_leaves_no_partial_target_or_staging_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let old = paths(&fixture.path().join("old"));
        let new = paths(&fixture.path().join("new"));
        write(&old.workflows_dir().join("flow.json"), b"workflow");
        std::os::unix::fs::symlink(fixture.path(), old.workflows_dir().join("link")).unwrap();
        assert!(matches!(
            migrate_legacy(&old, &new),
            Err(DirectoryError::UnsupportedFile)
        ));
        assert!(!new.workflows_dir().exists());
        assert_eq!(fs::read_dir(&new.data).unwrap().count(), 0);
        assert!(old.workflows_dir().join("flow.json").exists());
    }
}
