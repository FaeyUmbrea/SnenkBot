//! Configuration requests published by compiled integration modules.

use std::sync::{LockResult, PoisonError, RwLock, RwLockReadGuard};

use tokio::sync::watch;

/// A synchronous status snapshot with coalesced, secret-free change notifications.
pub struct StatusCell<T> {
    value: RwLock<T>,
    changed: watch::Sender<u64>,
}

impl<T: PartialEq> StatusCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            value: RwLock::new(value),
            changed: watch::channel(0).0,
        }
    }

    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        self.value.read()
    }

    /// Returns whether the committed snapshot changed. Poisoned writes are rejected.
    pub fn set(&self, value: T) -> LockResult<bool> {
        let mut current = self.value.write().map_err(|_| PoisonError::new(false))?;
        if *current == value {
            return Ok(false);
        }
        *current = value;
        // Subscribers can synchronously read the snapshot when they wake.
        drop(current);
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(true)
    }

    /// Subscribe before reading the snapshot so concurrent writes remain observable.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ReconfigurationRequest {
    pub integration: &'static str,
    pub connection: &'static str,
    pub reason: String,
    pub affected_features: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Inactive,
    Connecting,
    Connected,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConnectionStatus {
    pub integration: String,
    pub connection: String,
    pub title: String,
    pub state: ConnectionState,
    pub detail: String,
}

/// Requests describe current configuration, rather than transient notifications.
/// Modules keep publishing a request until the required setup is satisfied.
pub trait ConfigurationStatus: Send + Sync {
    fn reconfiguration_requests(&self) -> Vec<ReconfigurationRequest>;
    fn connection_statuses(&self) -> Vec<ConnectionStatus>;
    fn subscribe_configuration_changes(&self) -> Vec<watch::Receiver<u64>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_writes_notify_after_committing_and_coalesce() {
        let status = StatusCell::new("disabled".to_owned());
        let mut changes = status.subscribe();
        assert_eq!(*status.read().unwrap(), "disabled");
        assert!(!status.set("disabled".into()).unwrap());
        assert!(!changes.has_changed().unwrap());
        assert!(status.set("connecting".into()).unwrap());
        assert!(status.set("connected".into()).unwrap());
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow_and_update(), 2);
        assert_eq!(*status.read().unwrap(), "connected");
        assert!(!changes.has_changed().unwrap());
    }

    #[test]
    fn writes_without_subscribers_are_visible_to_later_snapshots() {
        let status = StatusCell::new(false);
        status.set(true).unwrap();
        let changes = status.subscribe();
        assert_eq!(*changes.borrow(), 1);
        assert!(*status.read().unwrap());
    }

    #[test]
    fn poisoned_write_does_not_publish_an_uncommitted_update() {
        let status = StatusCell::new(false);
        let changes = status.subscribe();
        let _ = std::panic::catch_unwind(|| {
            let _write = status.value.write().unwrap();
            panic!("poison the status lock");
        });
        assert!(status.set(true).is_err());
        assert!(!changes.has_changed().unwrap());
        assert!(!*status.value.read().unwrap_err().into_inner());
    }
}
